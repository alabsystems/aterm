// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    accounts::proposals::Proposals,
    catalog::Directory,
    table::{
        Session,
        store::Store,
        tests::{Lab, repair_commons},
    },
};
use aterm_secrets::commons::roster::Role;

fn target() -> Target {
    Target {
        intent: true,
        account: [1; 32],
        revision: [2; 32],
        provider: "google".into(),
        grant: [6; 16],
    }
}
fn requests(lab: &Lab, n: usize) -> Requests {
    Requests::at_until(
        lab.paths[n].clone(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
}

#[test]
fn generic_replica_carries_native_requests_without_using_their_schema() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let proposals = Proposals::at(lab.paths[0].clone());
    let first = proposals
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "ordinary".into(),
                value: Some(json!({"provider":"google","method":"api-key"})),
            }],
        )
        .unwrap();
    let id = requests(&lab, 0).request(&target(), [3; 32]).unwrap();
    assert_eq!(requests(&lab, 0).request(&target(), [3; 32]).unwrap(), id);
    assert_eq!(requests(&lab, 0).view().unwrap().requests.len(), 1);
    // Only the existing generic catalog/replica path runs on the second peer.
    repair_commons(&lab, 4);
    let bootstrap = lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[1], bootstrap.trust());
    let scope = descriptor(&bootstrap).unwrap().scope;
    let store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(store.inventory(&session).unwrap().count(), 1);
    drop(store);
    drop(session);
    assert_eq!(
        Proposals::at(lab.paths[1].clone())
            .snapshot()
            .unwrap()
            .cells[&("accounts".into(), "ordinary".into())]
            .candidates[0]
            .operation,
        first.id
    );
    let second = Proposals::at(lab.paths[1].clone())
        .write(
            [2; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "other".into(),
                value: Some(json!({"provider":"meta","method":"api-key"})),
            }],
        )
        .unwrap();
    assert_ne!(second.id, first.id);
    let received = requests(&lab, 1).view().unwrap();
    assert_eq!(received.requests.len(), 1);
    assert_eq!(received.requests[0].operation, id);
    assert_eq!(
        received.requests[0].requester,
        lab.bootstrap(0).request().unwrap().binding().clone()
    );
}

#[test]
fn requester_cannot_claim_another_actor_and_new_policy_is_a_new_request() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let db = requests(&lab, 0);
    db.request(&target(), [3; 32]).unwrap();
    db.request(&target(), [4; 32]).unwrap();
    assert_eq!(db.view().unwrap().requests.len(), 2);
    let foreign = lab.bootstrap(1).request().unwrap().binding().actor();
    crate::documents::application::with_store(
        &lab.paths[0],
        false,
        descriptor,
        |session, store, _| {
            store.commit_catalog(session, &command(foreign, &target(), [5; 32]).encode()?)?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(db.view().unwrap().requests.len(), 2);
}

#[test]
fn causal_joins_do_not_become_native_requests_or_freeze_the_writer() {
    let lab = Lab::new(&[Role::Writer]);
    let db = requests(&lab, 0);
    let first = db.request(&target(), [3; 32]).unwrap();
    let merged = crate::documents::application::with_store(
        &lab.paths[0],
        false,
        descriptor,
        |session, store, _| {
            store.commit_catalog(session, &Command::Merge { request: [7; 16] }.encode()?)
        },
    )
    .unwrap();
    let view = db.view().unwrap();
    assert_eq!(view.requests.len(), 1);
    assert_eq!(view.requests[0].operation, first);
    assert_eq!(db.request(&target(), [3; 32]).unwrap(), first);
    let second = db.request(&target(), [4; 32]).unwrap();
    assert_ne!(second, first);
    let view = db.view().unwrap();
    assert_eq!(view.requests.len(), 2);
    assert!(
        view.requests
            .iter()
            .any(|request| request.operation == first)
    );
    assert!(
        view.requests
            .iter()
            .any(|request| request.operation == second)
    );
    assert!(
        view.requests
            .iter()
            .all(|request| request.operation != merged.id)
    );
}
