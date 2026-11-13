// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::tests::{tmp, wires};
use astream_wire::sync::MAX_PARENTS;

struct Peers {
    _root: aterm_tempfile::TempDir,
    paths: Vec<PathBuf>,
}

impl Peers {
    fn new(count: usize) -> Self {
        let root = tmp();
        let paths = (0..count)
            .map(|n| root.path().join(format!("peer-{n}")))
            .collect::<Vec<_>>();
        super::init(&paths[0], "frontier").unwrap();
        let owner = Credentials::load(&paths[0]).unwrap();
        for path in paths.iter().skip(1) {
            let request = super::request(path).unwrap();
            super::join(
                path,
                &owner.invitation(&request).unwrap(),
                owner.manifest.root,
            )
            .unwrap();
        }
        Self { _root: root, paths }
    }

    fn receive(&self, peer: usize, records: &[Vec<u8>]) {
        let mut db = Database::open(&self.paths[peer]).unwrap();
        for record in records.iter().rev() {
            db.ingest(record).unwrap();
        }
    }

    fn collect(&self) -> Vec<Vec<u8>> {
        self.paths.iter().flat_map(|path| wires(path)).collect()
    }
}

fn write(request: u8, key: &str, value: Value) -> Command {
    Command::Write {
        request: [request; 16],
        changes: vec![Change {
            collection: "example".into(),
            key: key.into(),
            value: Some(value),
        }],
    }
}

#[test]
fn private_database_writes_and_retries_survive_wide_frontiers() {
    for (writers, covered_previous) in [
        (MAX_PARENTS + 1, false),
        (MAX_PARENTS + 1, true),
        (2 * MAX_PARENTS + 1, false),
    ] {
        let peers = Peers::new(writers);
        for (n, path) in peers.paths.iter().enumerate() {
            Database::open(path)
                .unwrap()
                .commit(write(1, "shared", json!(n)))
                .unwrap();
        }
        if covered_previous {
            peers.receive(1, &wires(&peers.paths[0]));
            Database::open(&peers.paths[1])
                .unwrap()
                .commit(write(2, "bridge", json!(true)))
                .unwrap();
        }
        peers.receive(0, &peers.collect());
        let mut db = Database::open(&peers.paths[0]).unwrap();
        let before = db.view();
        assert_eq!(
            before.causal.heads.len(),
            writers - usize::from(covered_previous)
        );
        assert_eq!(
            before.cells[&("example".into(), "shared".into())]
                .candidates
                .len(),
            writers
        );
        let count = db.history.index().inventory().count();
        assert!(db.commit(write(1, "reused", json!(true))).is_err());
        assert!(
            db.commit(Command::Write {
                request: [8; 16],
                changes: vec![]
            })
            .is_err()
        );
        assert_eq!(db.history.index().inventory().count(), count);
        let oversized = write(9, "oversized", json!("x".repeat(crypto::MAX_BODY)));
        assert!(Command::decode(&oversized.encode().unwrap()).is_err());
        assert!(db.commit(oversized).is_err());
        assert_eq!(db.history.index().inventory().count(), count);
        let request = write(3, "after", json!(true));
        let receipt = db.commit(request.clone()).unwrap();
        let after = db.view();
        assert!(after.invalid.is_empty());
        assert_eq!(
            after.cells[&("example".into(), "shared".into())],
            before.cells[&("example".into(), "shared".into())]
        );
        assert!(db.history.index().inventory().count() > count + 1);
        if writers > 2 * MAX_PARENTS {
            assert!(db.history.index().inventory().count() >= count + 3);
        }
        assert_eq!(db.commit(request).unwrap(), receipt);
        for id in db.history.index().inventory() {
            assert!(db.history.index().get(id).unwrap().parents.len() <= MAX_PARENTS);
        }
        drop(db);
        let retained = wires(&peers.paths[0]);
        peers.receive(1, &retained);
        let peer = Database::open(&peers.paths[1]).unwrap();
        assert_eq!(peer.view().cells, after.cells);
        drop(peer);
        assert_eq!(wires(&peers.paths[1]), retained);
    }
}

#[test]
fn private_database_fixed_share_receipts_do_not_need_the_whole_frontier() {
    let peers = Peers::new(MAX_PARENTS + 1);
    let actor = Credentials::load(&peers.paths[1]).unwrap().capability.actor;
    let grant = Database::open(&peers.paths[0])
        .unwrap()
        .allocate([1; 16], "window".into(), 3, BTreeMap::from([(actor, 3)]))
        .unwrap();
    let mut ids = Vec::new();
    for (n, path) in peers.paths.iter().enumerate() {
        ids.push(
            Database::open(path)
                .unwrap()
                .commit(write(2, "shared", json!(n)))
                .unwrap(),
        );
    }
    peers.receive(1, &peers.collect());
    let mut db = Database::open(&peers.paths[1]).unwrap();
    assert_eq!(db.view().causal.heads.len(), MAX_PARENTS + 1);
    let first = db.spend([3; 16], "window", 2).unwrap();
    assert_eq!(db.history.index().get(&first).unwrap().parents.len(), 2);
    assert_eq!(db.spend([3; 16], "window", 2).unwrap(), first);
    assert_eq!(db.view().spent[&(grant, actor)], 2);
    drop(db);

    peers.receive(0, &wires(&peers.paths[1]));
    Database::open(&peers.paths[0])
        .unwrap()
        .revoke_writes([4; 16], actor, Some(ids[1]))
        .unwrap();
    peers.receive(1, &wires(&peers.paths[0]));
    let mut db = Database::open(&peers.paths[1]).unwrap();
    let last = db.spend([5; 16], "window", 1).unwrap();
    assert!(db.history.index().get(&last).unwrap().parents.len() <= 2);
    assert_eq!(db.spend([5; 16], "window", 1).unwrap(), last);
    assert_eq!(db.view().spent[&(grant, actor)], 3);
    assert_eq!(db.view().liability[&(grant, actor)], 3);
    assert!(db.spend([6; 16], "window", 1).is_err());
    assert!(db.commit(write(7, "revoked", json!(true))).is_err());
    assert!(db.commit(Command::Merge { request: [8; 16] }).is_err());
    drop(db);
    peers.receive(0, &wires(&peers.paths[1]));
    let db = Database::open(&peers.paths[0]).unwrap();
    assert_eq!(db.view().spent[&(grant, actor)], 3);
    assert_eq!(db.view().liability[&(grant, actor)], 3);
}

#[test]
fn a_valid_spend_retry_survives_withdrawn_catalog_ancestry() {
    let peers = Peers::new(2);
    let actor = Credentials::load(&peers.paths[1]).unwrap().capability.actor;
    let grant = Database::open(&peers.paths[0])
        .unwrap()
        .allocate([1; 16], "window".into(), 2, BTreeMap::from([(actor, 2)]))
        .unwrap();
    let mut db = Database::open(&peers.paths[1]).unwrap();
    let allowed = db.commit(write(2, "allowed", json!(true))).unwrap();
    db.commit(write(3, "late", json!(true))).unwrap();
    drop(db);
    peers.receive(1, &wires(&peers.paths[0]));
    let receipt = Database::open(&peers.paths[1])
        .unwrap()
        .spend([4; 16], "window", 1)
        .unwrap();
    peers.receive(0, &wires(&peers.paths[1]));
    Database::open(&peers.paths[0])
        .unwrap()
        .revoke_writes([5; 16], actor, Some(allowed))
        .unwrap();
    peers.receive(1, &wires(&peers.paths[0]));

    let mut db = Database::open(&peers.paths[1]).unwrap();
    let view = db.view();
    assert!(!view.causal.ordered.contains(&receipt));
    assert_eq!(view.spent[&(grant, actor)], 1);
    assert_eq!(view.liability[&(grant, actor)], 1);
    let count = db.history.index().inventory().count();
    assert_eq!(db.spend([4; 16], "window", 1).unwrap(), receipt);
    assert_eq!(db.history.index().inventory().count(), count);
    assert_eq!(db.view().spent[&(grant, actor)], 1);
    let final_receipt = db.spend([6; 16], "window", 1).unwrap();
    assert!(!db.view().causal.ordered.contains(&final_receipt));
    assert_eq!(db.view().spent[&(grant, actor)], 2);
    assert_eq!(db.view().liability[&(grant, actor)], 2);
    let final_count = db.history.index().inventory().count();
    assert_eq!(db.spend([6; 16], "window", 1).unwrap(), final_receipt);
    assert_eq!(db.spend([4; 16], "window", 1).unwrap(), receipt);
    assert_eq!(db.history.index().inventory().count(), final_count);
    assert!(db.spend([7; 16], "window", 1).is_err());
    assert!(db.commit(write(8, "still-revoked", json!(true))).is_err());
}

#[test]
fn pending_budget_history_cannot_authorize_a_debit() {
    let peers = Peers::new(2);
    let actor = Credentials::load(&peers.paths[1]).unwrap().capability.actor;
    let (predecessor_wire, grant, grant_wire) = {
        let mut owner = Database::open(&peers.paths[0]).unwrap();
        let predecessor = owner
            .allocate([1; 16], "earlier".into(), 1, BTreeMap::from([(actor, 1)]))
            .unwrap();
        let grant = owner
            .allocate([2; 16], "window".into(), 2, BTreeMap::from([(actor, 2)]))
            .unwrap();
        (
            owner.history.wire(&predecessor).unwrap().to_vec(),
            grant,
            owner.history.wire(&grant).unwrap().to_vec(),
        )
    };
    peers.receive(1, &[grant_wire]);
    let mut db = Database::open(&peers.paths[1]).unwrap();
    let view = db.view();
    assert!(view.causal.pending.contains(&grant));
    assert_eq!(view.budgets["window"], vec![grant]);
    let count = db.history.index().inventory().count();
    let result = db.spend([3; 16], "window", 1);
    assert!(
        result.is_err(),
        "pending grant authorized a debit: {result:?}"
    );
    assert_eq!(db.history.index().inventory().count(), count);
    assert!(view.disputed_shares.contains(&(grant, actor)));

    db.ingest(&predecessor_wire).unwrap();
    assert!(!db.view().disputed_shares.contains(&(grant, actor)));
    let receipt = db.spend([3; 16], "window", 1).unwrap();
    assert_eq!(db.view().spent[&(grant, actor)], 1);
    assert_eq!(db.view().liability[&(grant, actor)], 1);
    let count = db.history.index().inventory().count();
    assert_eq!(db.spend([3; 16], "window", 1).unwrap(), receipt);
    assert_eq!(db.history.index().inventory().count(), count);
    assert!(db.view().invalid.is_empty());
}
