// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use astream_wire::sync::Index;
use aterm_secrets::commons::constraints::{Fact, Frontier};
use aterm_spec::interp::State;

fn measure(state: &State, scenario: i64, store: &Store, session: &Session<'_>) -> State {
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("retained", store.inventory(session).unwrap().count() as i64);
    next.insert(
        "offered",
        store.replication_inventory(session).unwrap().count() as i64,
    );
    next.insert("visible", store.catalog(session).unwrap().len() as i64);
    next.insert(
        "complete",
        i64::from(store.liability_evidence(session).is_ok()),
    );
    next.insert("forked", store.index.project().quarantined.len() as i64);
    next
}

fn closure_transition(state: &State, action: &str) -> State {
    if matches!(action, "TransitiveFork" | "GappedSuffix") {
        let gapped = action == "GappedSuffix";
        let f = Fixture::new(gapped, false, false);
        f.recover();
        let a = f.lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let mut store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
        let missing = [0xab; 32];
        let parents = if gapped { missing } else { f.receipt.id };
        let wire = session
            .seal(
                f.scope,
                Causal {
                    sequence: if gapped { 3 } else { 1 },
                    previous: gapped.then_some(missing),
                    parents: &[parents],
                },
                b"tainted selected record",
            )
            .unwrap();
        store.ingest(&session, &wire).unwrap();
        if !gapped {
            let free = session
                .seal(
                    f.scope,
                    Causal {
                        sequence: 1,
                        previous: None,
                        parents: &[],
                    },
                    b"competing independent branch",
                )
                .unwrap();
            store.ingest(&session, &free).unwrap();
            // Removing only the directly dependent branch launders this fork.
            let mut old = Index::new(4);
            let id = session.authenticate(f.scope, &free).unwrap().header().id;
            old.insert(store.index.get(&id).unwrap().clone()).unwrap();
            assert_eq!(old.project().ordered, [id]);
        }
        drop(store);
        let store = Store::open(&session, &f.dirs[0], f.scope).unwrap();
        return measure(state, if gapped { 2 } else { 1 }, &store, &session);
    }

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let dir = lab._home.path().join("closure");
    let session = Session::open(&a).unwrap();
    let peer = Session::open(&b).unwrap();
    let mut store = Store::create(&session, &dir).unwrap();
    let scope = store.scope();
    let first = store.commit_catalog(&session, b"first writer").unwrap();
    if action == "ImpliedFork" {
        let missing = [0xab; 32];
        let wire = session
            .seal(
                scope,
                Causal {
                    sequence: 2,
                    previous: Some(missing),
                    parents: &[missing],
                },
                b"claims a competing predecessor",
            )
            .unwrap();
        store.ingest(&session, &wire).unwrap();
        // Old actual-dot-only detection allowed the selected first record.
        let mut old = Index::new(2);
        old.insert(store.index.get(&first.id).unwrap().clone())
            .unwrap();
        assert_eq!(old.project().ordered, [first.id]);
        drop(store);
        let store = Store::open(&session, &dir, scope).unwrap();
        return measure(state, 3, &store, &session);
    }
    let wire = peer
        .seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"second writer",
        )
        .unwrap();
    let second = store.ingest(&session, &wire).unwrap();
    let cutoff = |actor, sequence, digest| Fact::Cutoff {
        table: scope.digest(),
        actor,
        through: Some(Frontier { sequence, digest }),
    };
    let (scenario, facts) = match action {
        "ValidFrontier" => (4, vec![cutoff(session.actor(), 1, first.id)]),
        "WrongActor" => (5, vec![cutoff(session.actor(), 1, second.id)]),
        "WrongSequence" => (6, vec![cutoff(session.actor(), 2, first.id)]),
        _ => panic!("unknown closure action"),
    };
    // Each digest exists and every causal parent is present. Existence alone
    // was the old, insufficient liability check for both invalid cases.
    assert!(store.index.project().pending.is_empty());
    for fact in &facts {
        if let Fact::Cutoff {
            through: Some(frontier),
            ..
        } = fact
        {
            assert!(store.records.contains_key(&frontier.digest));
        }
    }
    drop((store, session, peer));
    ingest(&lab, &rotation_with_facts(&lab, &facts));
    let session = Session::open(&a).unwrap();
    let store = Store::open(&session, &dir, scope).unwrap();
    measure(state, scenario, &store, &session)
}

#[test]
fn quarantine_closure_and_frontier_identity_conform_with_negative_controls() {
    let model = aterm_models_db::table_quarantine_closure_model();
    aterm_spec::interp::prove_and_catch(&model);
    let initial = model.init_state();
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for action in &model.actions {
        let actual = closure_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
        let mut unsafe_state = actual;
        let law = match action.name {
            "TransitiveFork" | "GappedSuffix" => {
                unsafe_state.insert("offered", 1);
                "TaintedHistoryNotOffered"
            }
            "ImpliedFork" => {
                unsafe_state.insert("forked", 0);
                unsafe_state.insert("visible", 1);
                "ImpliedForkRestricts"
            }
            "ValidFrontier" => {
                unsafe_state.insert("complete", 0);
                "ValidFrontierReadable"
            }
            _ => {
                unsafe_state.insert("complete", 1);
                "InvalidFrontierWithholds"
            }
        };
        assert!(!model.check_invariant(law, &unsafe_state));
    }
}
