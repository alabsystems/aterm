// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    database::{Change, Command},
    table::tests::{
        Lab,
        frontier::{receive, wide_lab, wires},
    },
};
use astream_wire::sync::MAX_PARENTS;
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

fn stopped(account: Digest) -> Request {
    Request {
        account,
        generation: 1,
        parents: vec![],
        action: Action::Stop,
    }
}

#[test]
fn activation_history_keeps_root_decisions_after_many_writers() {
    let writers = MAX_PARENTS + 1;
    let lab = wide_lab(writers);
    let admin = aterm_secrets::client::admin_open(&lab.paths[0]).unwrap();
    let mut expected = BTreeMap::new();
    for (writer, paths) in lab.paths.iter().enumerate() {
        let account = [u8::try_from(writer + 1).unwrap(); 32];
        let (decision, _) = Activations::at(paths.clone())
            .issue(&admin, [1; 16], stopped(account))
            .unwrap();
        expected.insert(account, State::Stopped(decision.id()));
    }
    let scope = descriptor(&lab.bootstrap(0)).unwrap().scope;
    let incoming = (1..writers)
        .flat_map(|writer| wires(&lab, writer, scope))
        .collect::<Vec<_>>();
    receive(&lab, 0, scope, &incoming);
    let db = Activations::at(lab.paths[0].clone());
    let history = db
        .observe_store(|session, store, scope| History::read(session, store, scope.trust()))
        .unwrap();
    assert_eq!(history.statuses(), expected);

    let request = stopped([250; 32]);
    let next = db.issue(&admin, [2; 16], request.clone()).unwrap();
    expected.insert(request.account, State::Stopped(next.0.id()));
    let retained = wires(&lab, 0, scope);
    assert!(retained.len() > writers + 1);
    let retry = db.issue(&admin, [2; 16], request).unwrap();
    assert_eq!(retry.0.bytes(), next.0.bytes());
    assert_eq!(retry.1, next.1);
    assert_eq!(wires(&lab, 0, scope), retained);

    for writer in 0..2 {
        if writer == 1 {
            receive(&lab, writer, scope, &retained);
        }
        let history = Activations::at(lab.paths[writer].clone())
            .observe_store(|session, store, scope| History::read(session, store, scope.trust()))
            .unwrap();
        assert_eq!(history.statuses(), expected);
        assert_eq!(history.ignored, 0);
        assert_eq!(history.unavailable, 0);
        assert_eq!(wires(&lab, writer, scope), retained);
    }
}

#[test]
fn only_canonical_joins_are_structural_in_activation_history() {
    let lab = Lab::new(&[Role::Writer]);
    let admin = aterm_secrets::client::admin_open(&lab.paths[0]).unwrap();
    let db = Activations::at(lab.paths[0].clone());
    let request = stopped([7; 32]);
    let root = db.issue(&admin, [1; 16], request.clone()).unwrap();
    let canonical = Command::Merge { request: [1; 16] }.encode().unwrap();
    let mut noncanonical = canonical.clone();
    noncanonical.push(b'\n');
    let foreign = Command::Write {
        request: [2; 16],
        changes: vec![Change {
            collection: "root".into(),
            key: "forged".into(),
            value: Some(json!({"stop":true})),
        }],
    }
    .encode()
    .unwrap();
    db.with_store(false, |session, store, _| {
        for payload in [
            canonical,
            noncanonical,
            foreign,
            b"AAOP1not-a-root-signed-decision".to_vec(),
        ] {
            store.commit_catalog(session, &payload)?;
        }
        Ok(())
    })
    .unwrap();
    let history = db
        .observe_store(|session, store, scope| History::read(session, store, scope.trust()))
        .unwrap();
    assert_eq!(history.records.len(), 1);
    assert_eq!(history.requests.len(), 1);
    assert_eq!(history.ignored, 3);
    assert_eq!(history.unavailable, 0);
    assert_eq!(history.status([7; 32]), State::Stopped(root.0.id()));
    let scope = descriptor(&lab.bootstrap(0)).unwrap().scope;
    let before = wires(&lab, 0, scope);
    let retry = db.issue(&admin, [1; 16], request).unwrap();
    assert_eq!(retry.0.bytes(), root.0.bytes());
    assert_eq!(retry.1, root.1);
    assert_eq!(wires(&lab, 0, scope), before);
}
